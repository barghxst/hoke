mod container;
mod journal;
mod limits;
mod read_view;
mod scan;
mod sst;
mod storage;
mod wal;

pub use container::WasmContainerDb;
pub use journal::WasmJournal;
pub use read_view::WasmReadView;
pub use sst::WasmSst;
pub use wal::WasmWal;

use wasm_bindgen::JsError;

fn binding_error(error: impl std::fmt::Display) -> JsError {
    JsError::new(&error.to_string())
}
