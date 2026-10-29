import type { KVBackend } from "../../src/kv/backend.js";

export class MemoryBackend implements KVBackend {
  private readonly entries = new Map<string, string>();

  read(key: string): string | undefined {
    return this.entries.get(key);
  }

  write(key: string, value: string): void {
    this.entries.set(key, value);
  }

  remove(key: string): void {
    this.entries.delete(key);
  }
}
