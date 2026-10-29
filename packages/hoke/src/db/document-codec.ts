import { isThenable, observePromise } from "../sync-result.js";
import { type CollectionOptions, DocumentError } from "./document-contract.js";
import { MAX_KEY_BYTES, MAX_VALUE_BYTES } from "./protocol.js";

const encoder = new TextEncoder();

const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

export function encodeString(value: unknown): Uint8Array {
  if (typeof value !== "string")
    throw new DocumentError("INVALID_ARGUMENT", "Expected string key or collection name");
  for (let i = 0; i < value.length; i++) {
    const unit = value.charCodeAt(i);
    if (unit >= 0xd800 && unit <= 0xdbff) {
      const next = value.charCodeAt(++i);
      if (!(next >= 0xdc00 && next <= 0xdfff))
        throw new DocumentError("INVALID_ARGUMENT", "String contains an unpaired surrogate");
    } else if (unit >= 0xdc00 && unit <= 0xdfff) {
      throw new DocumentError("INVALID_ARGUMENT", "String contains an unpaired surrogate");
    }
  }
  const bytes = encoder.encode(value);
  if (bytes.length > MAX_KEY_BYTES)
    throw new DocumentError("INVALID_ARGUMENT", "Encoded string exceeds key limit");
  return bytes;
}

export function decodeString(bytes: Uint8Array): string {
  try {
    return decoder.decode(bytes);
  } catch (cause) {
    throw new DocumentError("INVALID_DATA", "Stored key is not valid UTF-8", cause);
  }
}

function assertJSON(value: unknown, ancestors: Set<object>): void {
  if (value === null || typeof value === "string" || typeof value === "boolean") return;
  if (typeof value === "number" && Number.isFinite(value) && !Object.is(value, -0)) return;
  if (typeof value !== "object" || value === null)
    throw new TypeError("Expected lossless JSON value");
  const array = Array.isArray(value);
  if (Object.getPrototypeOf(value) !== (array ? Array.prototype : Object.prototype))
    throw new TypeError("Expected plain JSON object or array");

  for (let owner: object | null = value; owner !== null; owner = Object.getPrototypeOf(owner)) {
    const hook = Object.getOwnPropertyDescriptor(owner, "toJSON");
    if (!hook) continue;
    if (!("value" in hook) || typeof hook.value === "function")
      throw new TypeError("JSON conversion hooks are not supported");
    break;
  }
  if (ancestors.has(value)) throw new TypeError("JSON value contains a cycle");
  ancestors.add(value);
  const keys = Reflect.ownKeys(value);
  if (array && keys.length !== (value as unknown[]).length + 1)
    throw new TypeError("JSON array must be dense and have no extra properties");
  for (const key of keys) {
    if (array && key === "length") continue;
    if (typeof key !== "string") throw new TypeError("JSON cannot preserve symbol properties");
    if (array && (!/^(0|[1-9]\d*)$/.test(key) || Number(key) >= (value as unknown[]).length))
      throw new TypeError("JSON array has extra properties");
    const descriptor = Object.getOwnPropertyDescriptor(value, key);
    if (!descriptor || !("value" in descriptor) || !descriptor.enumerable)
      throw new TypeError("JSON requires enumerable data properties");
    assertJSON(descriptor.value, ancestors);
  }
  ancestors.delete(value);
}

function freezeJSON(value: unknown): void {
  if (value === null || typeof value !== "object") return;
  for (const child of Object.values(value)) freezeJSON(child);
  Object.freeze(value);
}

function validate<T>(value: unknown, guard: CollectionOptions<T>["validate"]): T {
  freezeJSON(value);
  const accepted: unknown = guard(value);
  const promise = observePromise(accepted);
  if (promise || isThenable(accepted) || accepted !== true)
    throw new TypeError("Collection validation must return true synchronously");
  return value as T;
}

export function encodeDocument<T>(value: T, guard: CollectionOptions<T>["validate"]): Uint8Array {
  try {
    assertJSON(value, new Set());
    const json = JSON.stringify(value);

    validate(JSON.parse(json), guard);
    const bytes = encoder.encode(json);
    if (bytes.length > MAX_VALUE_BYTES) throw new TypeError("Encoded document exceeds 16 MiB");
    return bytes;
  } catch (cause) {
    throw new DocumentError(
      "INVALID_VALUE",
      "Document must be valid lossless JSON matching its collection schema",
      cause,
    );
  }
}

export function decodeDocument<T>(bytes: Uint8Array, guard: CollectionOptions<T>["validate"]): T {
  try {
    const value: unknown = JSON.parse(decoder.decode(bytes));
    assertJSON(value, new Set());
    return validate(value, guard);
  } catch (cause) {
    throw new DocumentError(
      "INVALID_DATA",
      "Stored document is not valid UTF-8 JSON matching its collection schema",
      cause,
    );
  }
}
