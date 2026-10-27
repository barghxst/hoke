import type { KV } from "./kv/index.js";

export type StoreErrorCode =
  | "invalid-definition"
  | "invalid-defaults"
  | "invalid-envelope"
  | "invalid-value"
  | "future-version"
  | "migration-required"
  | "missing-migration"
  | "migration-failed"
  | "async-result"
  | "update-failed"
  | "clone-failed";

export class StoreError extends Error {
  constructor(
    public readonly code: StoreErrorCode,
    message: string,
    options?: ErrorOptions,
  ) {
    super(message, options);
    this.name = "StoreError";
  }
}

export type StoreMigration = (previous: unknown) => unknown;
export interface StoreDefinition<T> {
  readonly key: string;
  readonly version: number;
  readonly defaults: () => T;
  readonly validate: (value: unknown) => value is T;
  /** Destination version: migrations[2] converts version 1 to version 2. */
  readonly migrations?: Readonly<Record<number, StoreMigration>>;
}
export interface Store<T> {
  getSnapshot(): T;
  getServerSnapshot(): T;
  set(value: T): void;
  update(updater: (previous: T) => T): void;
  reset(): void;
  subscribe(listener: () => void): () => void;
}
export interface StoreEnvelope<T = unknown> {
  readonly __stowtree: 1;
  readonly version: number;
  readonly value: T;
}

function positiveVersion(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
}
function thenable(value: unknown): boolean {
  return (
    value !== null &&
    (typeof value === "object" || typeof value === "function") &&
    typeof (value as { then?: unknown }).then === "function"
  );
}
function checkDefinition<T>(definition: StoreDefinition<T>): void {
  if (
    !definition ||
    typeof definition.key !== "string" ||
    !definition.key.length ||
    !positiveVersion(definition.version) ||
    typeof definition.defaults !== "function" ||
    typeof definition.validate !== "function"
  ) {
    throw new StoreError(
      "invalid-definition",
      "Store requires a key, positive integer version, defaults factory and validator.",
    );
  }
  if (definition.migrations !== undefined) {
    if (definition.migrations === null || typeof definition.migrations !== "object") {
      throw new StoreError(
        "invalid-definition",
        "Migrations must be a map of destination versions to functions.",
      );
    }
    for (const [version, migration] of Object.entries(definition.migrations)) {
      if (
        !positiveVersion(Number(version)) ||
        Number(version) < 2 ||
        String(Number(version)) !== version ||
        typeof migration !== "function"
      ) {
        throw new StoreError("invalid-definition", `Invalid migration destination: ${version}`);
      }
    }
  }
}

export function defineStore<T>(definition: StoreDefinition<T>): StoreDefinition<T> {
  checkDefinition(definition);
  // Keep a caller's later configuration mutation from changing an opened store.
  return Object.freeze({
    ...definition,
    ...(definition.migrations === undefined
      ? {}
      : {
          migrations: Object.freeze({ ...definition.migrations }),
        }),
  });
}

function validateValue<T>(
  definition: StoreDefinition<T>,
  value: unknown,
  code: "invalid-value" | "invalid-defaults",
): asserts value is T {
  try {
    if (!thenable(value) && definition.validate(value)) return;
  } catch (cause) {
    throw new StoreError(code, `Validation failed for store: ${definition.key}`, { cause });
  }
  throw new StoreError(code, `Invalid value for store: ${definition.key}`);
}

function protectedCopy<T>(value: T): T {
  try {
    const copy = structuredClone(value);
    const visited = new WeakSet<object>();
    function freeze(item: unknown): void {
      if (!item || typeof item !== "object" || visited.has(item)) return;
      visited.add(item);
      for (const child of Object.values(item)) freeze(child);
      // Typed array elements cannot be frozen; the clone still isolates them.
      if (!ArrayBuffer.isView(item)) Object.freeze(item);
    }
    freeze(copy);
    return copy;
  } catch (cause) {
    throw new StoreError("clone-failed", "Store updates and migrations require cloneable values.", {
      cause,
    });
  }
}

function envelope(raw: unknown, key: string): StoreEnvelope {
  if (
    !raw ||
    typeof raw !== "object" ||
    Array.isArray(raw) ||
    !Object.hasOwn(raw, "__stowtree") ||
    !Object.hasOwn(raw, "version") ||
    !Object.hasOwn(raw, "value") ||
    (raw as StoreEnvelope).__stowtree !== 1 ||
    !positiveVersion((raw as StoreEnvelope).version)
  ) {
    throw new StoreError("invalid-envelope", `Invalid stored envelope for: ${key}`);
  }
  return raw as StoreEnvelope;
}

export function createStore<T>(kv: KV<unknown>, input: StoreDefinition<T>): Store<T> {
  const definition = defineStore(input);
  let defaults: T;
  try {
    defaults = definition.defaults();
  } catch (cause) {
    throw new StoreError("invalid-defaults", `Defaults factory failed for: ${definition.key}`, {
      cause,
    });
  }
  validateValue(definition, defaults, "invalid-defaults");
  let cachedRaw: unknown;
  let cachedValue = defaults;
  let hasCache = false;

  function read(allowMigration: boolean): T {
    // Always consult the backend, including after failed reads and external writes.
    const raw = kv.get(definition.key);
    if (raw === undefined) {
      cachedRaw = undefined;
      cachedValue = defaults;
      hasCache = true;
      return defaults;
    }
    if (hasCache && Object.is(raw, cachedRaw)) return cachedValue;
    const stored = envelope(raw, definition.key);
    if (stored.version > definition.version) {
      throw new StoreError(
        "future-version",
        `Store ${definition.key} has newer version ${stored.version}.`,
      );
    }
    if (stored.version < definition.version) {
      if (!allowMigration) {
        throw new StoreError(
          "migration-required",
          `Reopen store ${definition.key} to migrate version ${stored.version}.`,
        );
      }
      // Verify the complete path before executing user functions.
      const migrations: { version: number; migrate: StoreMigration }[] = [];
      for (let version = stored.version + 1; version <= definition.version; version++) {
        const migrate = definition.migrations?.[version];
        if (!Object.hasOwn(definition.migrations ?? {}, version) || !migrate) {
          throw new StoreError(
            "missing-migration",
            `Missing migration to version ${version} for: ${definition.key}`,
          );
        }
        migrations.push({ version, migrate });
      }
      let value: unknown = stored.value;
      for (const { version, migrate } of migrations) {
        const previous = protectedCopy(value);
        try {
          value = migrate(previous);
          if (thenable(value)) {
            throw new StoreError(
              "async-result",
              `Migration to version ${version} must be synchronous.`,
            );
          }
        } catch (cause) {
          if (cause instanceof StoreError && cause.code === "async-result") throw cause;
          throw new StoreError(
            "migration-failed",
            `Migration to version ${version} failed for: ${definition.key}`,
            { cause },
          );
        }
      }
      validateValue(definition, value, "invalid-value");
      kv.set(definition.key, {
        __stowtree: 1,
        version: definition.version,
        value,
      } satisfies StoreEnvelope<T>);
      // Read back the actual backend value: serialization and listeners can change it.
      return read(false);
    }
    validateValue(definition, stored.value, "invalid-value");
    cachedRaw = raw;
    cachedValue = stored.value;
    hasCache = true;
    return stored.value;
  }

  function set(value: T): void {
    // Refuse to overwrite externally replaced incompatible or corrupt data.
    read(false);
    validateValue(definition, value, "invalid-value");
    kv.set(definition.key, {
      __stowtree: 1,
      version: definition.version,
      value,
    } satisfies StoreEnvelope<T>);
  }
  read(true);
  return {
    getSnapshot: () => read(false),
    getServerSnapshot: () => defaults,
    set,
    update(updater) {
      const previous = protectedCopy(read(false));
      let next: T;
      try {
        next = updater(previous);
        if (thenable(next))
          throw new StoreError("async-result", "Store updater must be synchronous.");
      } catch (cause) {
        if (cause instanceof StoreError && cause.code === "async-result") throw cause;
        throw new StoreError("update-failed", `Updater failed for: ${definition.key}`, { cause });
      }
      set(next);
    },
    reset() {
      read(false);
      kv.delete(definition.key);
    },
    subscribe: (listener) => kv.subscribe(definition.key, listener),
  };
}
