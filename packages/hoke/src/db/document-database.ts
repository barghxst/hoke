import { type BrowserDB, DBError, type DBOperationOptions } from "./client-contract.js";
import { encodeString } from "./document-codec.js";
import { createDocumentCollection } from "./document-collection.js";
import { type Collection, type CollectionOptions, DocumentError } from "./document-contract.js";
import { DocumentSession } from "./document-session.js";
import { openDB } from "./open-byte-database.js";

export interface OpenDatabaseOptions extends DBOperationOptions {
  timeoutMs?: number;
}

/** Typed JSON collections backed by one exclusively owned container database. */
export interface Database {
  collection<T>(name: string, options: CollectionOptions<T>): Collection<T>;
  close(): Promise<void>;
}

/** Internal composition point; the public entry point opens the container itself. */
export function createDatabase(raw: BrowserDB): Database {
  if (raw.mode !== "container-v1") {
    throw new DBError("UNSUPPORTED", "Document databases require container-v1");
  }
  const session = new DocumentSession(raw);
  return {
    collection<T>(name: string, options: CollectionOptions<T>): Collection<T> {
      session.assertOpen();
      const prefix = encodeString(name);
      if (prefix.length === 0 || prefix.length > 65_531) {
        throw new DocumentError(
          "INVALID_ARGUMENT",
          "Collection name must fit within 65531 UTF-8 bytes and be nonempty",
        );
      }
      const collection = createDocumentCollection(
        raw.collection(prefix),
        options,
        session.execute,
        5 + prefix.length,
      );
      return {
        ...collection,
        subscribe(key, listener) {
          session.assertOpen();
          return collection.subscribe(key, listener);
        },
        subscribeRange(range, listener) {
          session.assertOpen();
          return collection.subscribeRange(range, listener);
        },
      };
    },
    close: () => session.close(),
  };
}

/** Open JSON collections without choosing binary codecs or a storage mode. */
export async function openDatabase(
  name: string,
  options: OpenDatabaseOptions = {},
): Promise<Database> {
  if (
    options === null ||
    typeof options !== "object" ||
    Array.isArray(options) ||
    Reflect.ownKeys(options).some((key) => key !== "timeoutMs" && key !== "signal")
  ) {
    throw new DBError("INVALID_ARGUMENT", "Expected database timeoutMs/signal options");
  }
  const raw = await openDB(name, { ...options, mode: "container-v1" });
  return createDatabase(raw);
}
