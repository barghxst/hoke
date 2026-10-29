import type { KVBackend, KVBackendOption } from "./backend.js";

function checkBackend(backend: KVBackend): KVBackend {
  if (
    backend === null ||
    typeof backend !== "object" ||
    typeof backend.read !== "function" ||
    typeof backend.write !== "function" ||
    typeof backend.remove !== "function"
  ) {
    throw new TypeError("KV backend requires synchronous read, write and remove methods");
  }
  return backend;
}

export function selectBackend(
  backend: KVBackendOption | undefined,
  platform: "web" | "native",
): KVBackend | undefined {
  if (backend === undefined) return undefined;
  if (backend === null || typeof backend !== "object") {
    throw new TypeError("KV backend must be a backend or a platform map");
  }
  if ("read" in backend || "write" in backend || "remove" in backend) {
    return checkBackend(backend as KVBackend);
  }
  const selected = backend[platform];
  return selected === undefined ? undefined : checkBackend(selected);
}
