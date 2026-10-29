import initWasm from "../../wasm/hoke_wasm.js";

let loading: Promise<unknown> | undefined;
export function initializeWasm(): Promise<unknown> {
  loading ??= initWasm().catch((error: unknown) => {
    loading = undefined;
    throw error;
  });
  return loading;
}
