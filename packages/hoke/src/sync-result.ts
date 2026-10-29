export function observePromise(value: unknown): boolean {
  if (value === null || (typeof value !== "object" && typeof value !== "function")) return false;
  try {
    Promise.prototype.then.call(value, undefined, () => {});
    return true;
  } catch {
    return false;
  }
}

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
