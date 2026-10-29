import type { WasmSst } from "../../wasm/hoke_wasm.js";
import { MAX_VALUE_BYTES } from "./protocol.js";
import { type DBScanOptions, normalizeScanOptions } from "./scan.js";
import { decode, type SstEntry, validKey } from "./sst-packet.js";
import { SstFileError, type SstSyncHandle } from "./sst-storage.js";

export class SstFile {
  private engine: WasmSst | undefined;
  private handle: SstSyncHandle | undefined;
  constructor(
    readonly name: string,
    engine: WasmSst,
    handle: SstSyncHandle,
  ) {
    this.engine = engine;
    this.handle = handle;
  }
  private current(): WasmSst {
    if (!this.engine) throw new SstFileError("SST file is closed", undefined);
    return this.engine;
  }

  lookup(key: Uint8Array): Uint8Array | null | undefined {
    validKey(key);
    const result = this.current().lookup(key);
    if (result.length === 1 && result[0] === 0) return undefined;
    if (result.length === 1 && result[0] === 1) return null;
    if (result.length >= 1 && result.length <= MAX_VALUE_BYTES + 1 && result[0] === 2) {
      return result.slice(1);
    }
    throw new SstFileError("Invalid SST lookup tag", undefined);
  }
  scan(options: DBScanOptions = {}): SstEntry[] {
    const { start, end, prefix, reverse, limit } = normalizeScanOptions(options);
    const flags =
      (start && !start.inclusive ? 1 : 0) | (end?.inclusive ? 2 : 0) | (reverse ? 4 : 0);
    return decode(this.current().scan(start?.key, end?.key, prefix, flags, limit), limit);
  }
  close(): void {
    const engine = this.engine;
    const handle = this.handle;
    this.engine = undefined;
    this.handle = undefined;
    let failure: unknown;
    try {
      engine?.close();
    } catch (error) {
      failure = error;
    } finally {
      try {
        engine?.free();
      } catch (error) {
        failure ??= error;
      } finally {
        try {
          handle?.close();
        } catch (error) {
          failure ??= error;
        }
      }
    }
    if (failure !== undefined) throw failure;
  }
}
