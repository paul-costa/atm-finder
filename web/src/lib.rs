//! atm-finder web: the ATM exporter running in the browser as WebAssembly.
//!
//! The engine (boundaries, Overpass queries, merging, address matching, exports) comes from the
//! `atm-finder-core` crate. This crate adds what only exists in a browser: `engine` sends the
//! requests with `fetch`, `ui` wires up the page in `static/index.html`, and `i18n` holds the
//! German and English strings. `start` runs when the module has loaded.

pub mod i18n;

#[cfg(target_arch = "wasm32")]
pub mod engine;
#[cfg(target_arch = "wasm32")]
pub mod ui;

/// Entry point called by the generated JavaScript glue once the module is instantiated.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    // Panics otherwise disappear; with the hook they show up in the browser console.
    console_error_panic_hook::set_once();
    ui::mount();
}
