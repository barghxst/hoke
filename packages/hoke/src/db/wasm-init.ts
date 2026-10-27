import initWasm from "../../wasm/hoke_wasm.js";

// One instance per Worker realm: concurrently initialized bindings must not reset
// generated globals while another caller already owns live Rust allocations.
let loading: Promise<unknown> | undefined;
export function initializeWasm(): Promise<unknown> {
  loading ??= initWasm().catch((error: unknown) => {
    loading = undefined;
    throw error;
  });
  return loading;
}
