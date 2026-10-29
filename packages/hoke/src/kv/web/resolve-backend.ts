import type { KVBackend, KVBackendOption } from "../backend.js";
import { selectBackend } from "../select-backend.js";
import { LocalStorageBackend } from "./local-storage-backend.js";

export function resolveBackend(backend?: KVBackendOption): KVBackend {
  return selectBackend(backend, "web") ?? new LocalStorageBackend();
}
