//! Worker-only bindings over the platform-independent storage engine.
//!
//! Each binding module owns one domain: WAL, SST, read views, the publication
//! journal, or the container database. Shared modules translate scan packets
//! and synchronous JavaScript storage I/O; they do not implement persistence.
//!
//! The caller owns and closes the OPFS handle. No filesystem fallback is
//! provided; JavaScript adapter exceptions become engine I/O errors.

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
