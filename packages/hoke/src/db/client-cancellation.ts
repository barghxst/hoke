import { DBError, type DBOperationOptions } from "./client-contract.js";

export type Cancellation = {
  aborted: () => boolean;
  add: (listener: () => void) => void;
  remove: (listener: () => void) => void;
};

export function cancellation(options?: DBOperationOptions): Cancellation | undefined {
  try {
    if (options === undefined) return undefined;
    if (typeof options !== "object" || options === null) throw new Error();
    const signal = options.signal;
    if (signal === undefined) return undefined;
    if (typeof signal !== "object" || signal === null || typeof signal.aborted !== "boolean")
      throw new Error();
    const add = signal.addEventListener;
    const remove = signal.removeEventListener;
    if (typeof add !== "function" || typeof remove !== "function") throw new Error();
    return {
      aborted: () => {
        const aborted = signal.aborted;
        if (typeof aborted !== "boolean") throw new Error();
        return aborted;
      },
      add: (listener) => add.call(signal, "abort", listener, { once: true }),
      remove: (listener) => remove.call(signal, "abort", listener),
    };
  } catch {
    throw new DBError("INVALID_ARGUMENT", "Expected a structural abort signal");
  }
}

export function abortedError(): DBError {
  return new DBError(
    "ABORTED",
    "Database request aborted; dispatched mutation outcome may be unknown",
  );
}
