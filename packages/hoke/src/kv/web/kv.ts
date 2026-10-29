import type { KVOptions } from "../backend.js";
import { checkKVOptions, KVCore } from "../kv.js";
import { LocalStorageBackend } from "./local-storage-backend.js";

export class KV<Value = unknown> extends KVCore<Value> {
  constructor(options: KVOptions<Value>) {
    checkKVOptions(options);
    super({ ...options, backend: options.backend ?? new LocalStorageBackend() });
  }
}
