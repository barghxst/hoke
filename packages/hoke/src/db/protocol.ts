import type { DBBatchOperation } from "./batch.js";
import type { NormalizedScanOptions } from "./scan.js";

export const PROTOCOL_VERSION = 3;
export const MAX_KEY_BYTES = 65_536;
export const MAX_VALUE_BYTES = 16_777_216;

export type DBMode = "wal-v1" | "container-v1";

export type DBErrorCode =
  | "INVALID_ARGUMENT"
  | "UNSUPPORTED"
  | "LOCKED"
  | "STORAGE"
  | "ENGINE"
  | "CLOSED"
  | "ABORTED"
  | "TIMEOUT"
  | "WORKER"
  | "PROTOCOL";

export type Request = { version: 3; id: number } & (
  | { op: "open"; name: string; mode?: DBMode }
  | { op: "get" | "delete"; key: Uint8Array }
  | { op: "put"; key: Uint8Array; value: Uint8Array }
  | { op: "scan"; options: NormalizedScanOptions }
  | { op: "batch"; operations: DBBatchOperation[] }
  | { op: "flush" | "compact" | "stats" }
  | { op: "close" }
);

export type Response = { version: 3; id: number } & (
  | { ok: true; result: Uint8Array | null }
  | { ok: false; error: { code: DBErrorCode; message: string } }
);

export function validName(name: unknown): name is string {
  return typeof name === "string" && /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(name);
}
