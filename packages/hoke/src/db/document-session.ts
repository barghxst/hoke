import { type BrowserDB, DBError, type DBOperationOptions } from "./client-contract.js";

/** One FIFO for document reads, writes, maintenance and close. Never retries writes. */
export class DocumentSession {
  private pending: Promise<void> = Promise.resolve();
  private closing = false;
  private closePromise: Promise<void> | undefined;

  constructor(private readonly raw: BrowserDB) {}

  assertOpen(): void {
    if (this.closing) throw new DBError("CLOSED", "The database is closing or closed");
  }

  execute = <T>(
    operation: () => Promise<T>,
    options?: DBOperationOptions,
    mutation = false,
  ): Promise<T> => {
    try {
      this.assertOpen();
    } catch (error) {
      return Promise.reject(error);
    }
    const result = this.pending.then(async () => {
      // Maintain before the requested mutation: a failed/unknown commit is never retried.
      // Thresholds leave headroom below the core's 64-table / 64-WAL source caps.
      if (mutation) {
        const stats = await this.raw.stats(options);
        if (stats.tableCount >= 32 || stats.walSegments >= 48) {
          await this.raw.compact(options);
        }
      }
      return operation();
    });
    // A failed operation must not leave a permanently rejected queue. The raw
    // connection remains responsible for CLOSED/poisoned/unknown-outcome errors.
    this.pending = result.then(
      () => {},
      () => {},
    );
    return result;
  };

  close(): Promise<void> {
    if (this.closePromise) return this.closePromise;
    this.closing = true;
    this.closePromise = this.pending.then(() => this.raw.close());
    return this.closePromise;
  }
}
