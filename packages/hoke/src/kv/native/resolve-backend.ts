import type { KVBackend, KVBackendOption } from "../backend.js";
import { selectBackend } from "../select-backend.js";
import { MMKVBackend } from "./mmkv-backend.js";

export function resolveBackend(backend?: KVBackendOption): KVBackend {
  return selectBackend(backend, "native") ?? new MMKVBackend();
}
