import type { NormalizedScanOptions } from "./scan.js";

export const PROTOCOL_VERSION = 2;
export const MAX_KEY_BYTES = 65_536;
export const MAX_VALUE_BYTES = 16_777_216;

export type DBErrorCode =
  | "INVALID_ARGUMENT"
  | "UNSUPPORTED"
  | "LOCKED"
  | "STORAGE"
  | "ENGINE"
  | "CLOSED"
  | "TIMEOUT"
  | "WORKER"
  | "PROTOCOL";

export type Request = { version: 2; id: number } & (
  | { op: "open"; name: string }
  | { op: "get" | "delete"; key: Uint8Array }
  | { op: "put"; key: Uint8Array; value: Uint8Array }
  | { op: "scan"; options: NormalizedScanOptions }
  | { op: "close" }
);

export type Response = { version: 2; id: number } & (
  | { ok: true; result: Uint8Array | null }
  | { ok: false; error: { code: DBErrorCode; message: string } }
);

export function validName(name: unknown): name is string {
  return typeof name === "string" && /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(name);
}
