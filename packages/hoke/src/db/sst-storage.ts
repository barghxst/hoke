export interface SstSyncHandle {
  getSize(): number;
  read(buffer: Uint8Array, options: { at: number }): number;
  write(buffer: Uint8Array, options: { at: number }): number;
  truncate(size: number): void;
  flush(): void;
  close(): void;
}
export interface SstFileHandle {
  createSyncAccessHandle(): Promise<SstSyncHandle>;
}
export interface SstDirectory {
  getFileHandle(name: string, options: { create: boolean }): Promise<SstFileHandle>;
}
export class SstFileError extends Error {
  constructor(
    message: string,
    readonly candidateName: string | undefined,
    options?: ErrorOptions,
  ) {
    super(message, options);
    this.name = "SstFileError";
  }
}

export function validName(name: string): void {
  if (typeof name !== "string" || !/^[A-Za-z0-9_-]+\.sst$/.test(name)) {
    throw new TypeError("Expected a single SST filename ending in .sst");
  }
}
export function adapter(handle: SstSyncHandle) {
  return {
    getSize: () => handle.getSize(),
    readAt: (buffer: Uint8Array, at: number) => handle.read(buffer, { at }),
    writeAt: (buffer: Uint8Array, at: number) => handle.write(buffer, { at }),
    truncate: (size: number) => handle.truncate(size),
    flush: () => handle.flush(),
  };
}
