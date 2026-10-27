/** Observe genuine Promise rejections without invoking user-defined thenables. */
export function observePromise(value: unknown): boolean {
  if (value === null || (typeof value !== "object" && typeof value !== "function")) return false;
  try {
    // The native brand check supports promises from other realms, too.
    Promise.prototype.then.call(value, undefined, () => {});
    return true;
  } catch {
    // Arbitrary objects and thenables are not native Promise receivers.
    return false;
  }
}

/** Detect unsupported async results and prevent native unhandled rejections. */
export function isThenable(value: unknown): boolean {
  if (
    value === null ||
    (typeof value !== "object" && typeof value !== "function") ||
    typeof (value as { then?: unknown }).then !== "function"
  ) {
    return false;
  }
  observePromise(value);
  return true;
}
