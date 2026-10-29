import { DBError, type DBStats } from "./client-contract.js";
import { type DBErrorCode, MAX_VALUE_BYTES, type Request } from "./protocol.js";
import type { DBEntry, NormalizedScanOptions } from "./scan.js";
import { decodeScan } from "./scan-packet.js";

export type Result = Uint8Array | DBEntry[] | DBStats | null;

const ERROR_CODES = new Set<DBErrorCode>([
  "INVALID_ARGUMENT",
  "UNSUPPORTED",
  "LOCKED",
  "STORAGE",
  "ENGINE",
  "CLOSED",
  "TIMEOUT",
  "ABORTED",
  "WORKER",
  "PROTOCOL",
]);

function decodeStats(packet: unknown): DBStats {
  if (!(packet instanceof Uint8Array) || packet.byteLength !== 40) {
    throw new Error("Invalid stats packet");
  }
  const view = new DataView(packet.buffer, packet.byteOffset, packet.byteLength);
  const generation = view.getBigUint64(0, true);
  const limits = [64n, 64n, 1_000_000n, 33_554_432n];
  const counters = limits.map((limit, index) => {
    const value = view.getBigUint64((index + 1) * 8, true);
    if (value > limit || value > BigInt(Number.MAX_SAFE_INTEGER))
      throw new Error("Invalid stats counter");
    return Number(value);
  });
  if (generation < 1n) throw new Error("Invalid stats generation");
  return {
    generation,
    tableCount: counters[0] as number,
    walSegments: counters[1] as number,
    memtableEntries: counters[2] as number,
    memtableBytes: counters[3] as number,
  };
}

export function decodeResult(
  op: Request["op"],
  packet: unknown,
  scanOptions?: NormalizedScanOptions,
): Result {
  if (op === "stats") return decodeStats(packet);
  if (op === "scan" && scanOptions) return decodeScan(packet, scanOptions);
  if (packet === null) return null;
  if (op === "get" && packet instanceof Uint8Array && packet.byteLength <= MAX_VALUE_BYTES) {
    return new Uint8Array(packet);
  }
  throw new Error("Invalid response result");
}

export function decodeError(error: unknown): DBError {
  if (typeof error !== "object" || error === null) throw new Error("Invalid response error");
  const detail = error as Record<string, unknown>;
  if (
    typeof detail.code !== "string" ||
    !ERROR_CODES.has(detail.code as DBErrorCode) ||
    typeof detail.message !== "string"
  )
    throw new Error("Invalid response error");
  return new DBError(detail.code as DBErrorCode, detail.message);
}
